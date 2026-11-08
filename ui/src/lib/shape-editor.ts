// Visual SHACL authoring edits the RDF graph, never a reduced projection of a shape.
// Unchanged RDF, including unknown constraints and shared blank nodes, stays untouched.
import type { SparklesResult, Term } from './api';
import { patchTerm } from './support-patch';
import { iri, WELL_KNOWN, type PrefixMap } from './rdf';

export type Triple = [Term, Term, Term];
export const SH = WELL_KNOWN.sh;
export const RDF = WELL_KNOWN.rdf;
export const XSD = WELL_KNOWN.xsd;
export const MAX_TRIPLES = 5000;
export const MAX_BYTES = 2 * 1024 * 1024;
export const MAX_DEPTH = 64;
/** The most steps one expanded property path may have, counting each shared operand per use. */
export const MAX_PATH_NODES = 1000;
export const MAX_SAMPLES = 100;
export const MAX_TREE_REFERENCES = 5000;
export const id = (t: Term) => patchTerm(t);
export const same = (a: Term, b: Term) => id(a) === id(b);
const tripleId = (t: Triple) => t.map(id).join(' ');
export const literal = (value: string, datatype = XSD + 'string'): Term => ({
  type: 'literal',
  value,
  datatype,
});
export const isResource = (t: Term) => t.type === 'uri' || t.type === 'bnode';
function resources(term: Term): Term[] {
  const out: Term[] = [],
    todo = [term];
  while (todo.length) {
    const t = todo.pop()!;
    if (isResource(t)) out.push(t);
    else if (t.type === 'triple') todo.push(t.value.subject, t.value.predicate, t.value.object);
  }
  return out;
}
const nil = iri(RDF + 'nil');
const first = RDF + 'first';
const rest = RDF + 'rest';

export type ValueKind =
  | 'iri'
  | 'term'
  | 'text'
  | 'boolean'
  | 'integer'
  | 'decimal'
  | 'shape'
  | 'terms'
  | 'iris'
  | 'languages'
  | 'shapes';
export type Field = {
  name: string;
  label: string;
  kind: ValueKind;
  group: 'Targets' | 'Description' | 'Constraints';
  multiple?: boolean;
  propertyOnly?: boolean;
};
const field = (
  name: string,
  kind: ValueKind,
  group: Field['group'] = 'Constraints',
  extra: Partial<Field> = {},
): Field => ({ name, label: name, kind, group, ...extra });
export const FIELDS: Field[] = [
  ...['targetClass', 'targetSubjectsOf', 'targetObjectsOf'].map((n) =>
    field(n, 'iri', 'Targets', { multiple: true }),
  ),
  field('targetNode', 'term', 'Targets', { multiple: true }),
  ...['name', 'description', 'message'].map((n) =>
    field(n, 'text', 'Description', { multiple: true }),
  ),
  field('severity', 'iri', 'Description'),
  field('deactivated', 'boolean', 'Description'),
  field('order', 'decimal', 'Description'),
  field('group', 'iri', 'Description'),
  field('defaultValue', 'term', 'Description'),
  field('class', 'iri', 'Constraints', { multiple: true }),
  field('datatype', 'iri'),
  field('nodeKind', 'iri'),
  ...['minCount', 'maxCount'].map((n) =>
    field(n, 'integer', 'Constraints', { propertyOnly: true }),
  ),
  ...['minExclusive', 'minInclusive', 'maxExclusive', 'maxInclusive'].map((n) => field(n, 'term')),
  ...['minLength', 'maxLength'].map((n) => field(n, 'integer')),
  field('pattern', 'text', 'Constraints', { multiple: true }),
  field('flags', 'text'),
  field('languageIn', 'languages'),
  field('uniqueLang', 'boolean', 'Constraints', { propertyOnly: true }),
  ...['equals', 'disjoint'].map((n) => field(n, 'iri', 'Constraints', { multiple: true })),
  ...['lessThan', 'lessThanOrEquals'].map((n) =>
    field(n, 'iri', 'Constraints', { multiple: true, propertyOnly: true }),
  ),
  ...['and', 'or', 'xone'].map((n) => field(n, 'shapes', 'Constraints', { multiple: true })),
  ...['not', 'node'].map((n) => field(n, 'shape', 'Constraints', { multiple: true })),
  field('qualifiedValueShape', 'shape', 'Constraints', { propertyOnly: true }),
  ...['qualifiedMinCount', 'qualifiedMaxCount'].map((n) =>
    field(n, 'integer', 'Constraints', { propertyOnly: true }),
  ),
  field('qualifiedValueShapesDisjoint', 'boolean', 'Constraints', { propertyOnly: true }),
  field('closed', 'boolean'),
  field('ignoredProperties', 'iris'),
  field('hasValue', 'term', 'Constraints', { multiple: true }),
  field('in', 'terms'),
  field('memberShape', 'shape'),
  field('minListLength', 'integer'),
  field('maxListLength', 'integer'),
  field('uniqueMembers', 'boolean'),
];
export const FIELD_BY_NAME = new Map(FIELDS.map((f) => [f.name, f]));
export const listKind = (kind: ValueKind) =>
  ['terms', 'iris', 'languages', 'shapes'].includes(kind);
const shapeLinks = new Set(['property', 'node', 'not', 'qualifiedValueShape', 'memberShape']);
const logicalLinks = new Set(['and', 'or', 'xone']);
export const PATH_OPS = [
  'predicate',
  'inversePath',
  'sequence',
  'alternativePath',
  'zeroOrMorePath',
  'oneOrMorePath',
  'zeroOrOnePath',
] as const;
export type PathKind = (typeof PATH_OPS)[number];
export type ShapePath = { kind: PathKind; iri?: string; children?: ShapePath[] };
export const PATH_LABELS: Record<PathKind, string> = {
  predicate: 'Predicate',
  inversePath: 'Inverse',
  sequence: 'Sequence',
  alternativePath: 'Alternative',
  zeroOrMorePath: 'Zero or more',
  oneOrMorePath: 'One or more',
  zeroOrOnePath: 'Optional',
};
const structural = new Set([
  first,
  rest,
  ...PATH_OPS.filter((p) => p !== 'predicate' && p !== 'sequence').map((p) => SH + p),
]);

/** Full IRIs and declared prefixed names, with no silent percent-encoding. */
export function parseIri(text: string, prefixes: PrefixMap): string {
  const s = text.trim();
  const bare = s.startsWith('<') && s.endsWith('>') ? s.slice(1, -1) : s;
  const colon = bare.indexOf(':');
  const ns = !s.startsWith('<') && colon >= 0 ? prefixes[bare.slice(0, colon)] : undefined;
  const value = ns === undefined ? bare : ns + bare.slice(colon + 1);
  if (!/^[A-Za-z][A-Za-z0-9+.-]*:[^\s<>"{}|^`\\]*$/.test(value))
    throw new Error('Enter an absolute IRI or a declared prefixed name.');
  return value;
}

/** A typed field's value. Text literals keep their language or datatype explicitly. */
export function inputTerm(
  kind: ValueKind,
  text: string,
  prefixes: PrefixMap,
  type = 'literal',
  qualifier = '',
  direction = '',
): Term {
  if (kind === 'iri' || (kind === 'shape' && type !== 'bnode') || type === 'uri')
    return iri(parseIri(text, prefixes));
  if (type === 'bnode') {
    const label = text.trim().replace(/^_:/, '');
    if (!/^[A-Za-z0-9_](?:[A-Za-z0-9_.-]*[A-Za-z0-9_-])?$/.test(label))
      throw new Error('Enter a blank-node label.');
    return { type: 'bnode', value: label };
  }
  if (kind === 'boolean') {
    if (!['true', 'false'].includes(text)) throw new Error('Choose true or false.');
    return literal(text, XSD + 'boolean');
  }
  if (kind === 'integer') {
    if (!/^(?:\+?\d+|-0+)$/.test(text.trim())) throw new Error('Enter a nonnegative integer.');
    return literal(text.trim(), XSD + 'integer');
  }
  if (kind === 'decimal') {
    if (!/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(text.trim()))
      throw new Error('Enter a decimal number.');
    return literal(text.trim(), XSD + 'decimal');
  }
  if (qualifier.startsWith('@')) {
    const lang = qualifier.slice(1);
    if (!/^[A-Za-z]+(?:-[A-Za-z0-9]+)*$/.test(lang))
      throw new Error('Enter a language tag such as @en.');
    if (direction && direction !== 'ltr' && direction !== 'rtl')
      throw new Error('Choose a valid text direction.');
    return {
      type: 'literal',
      value: text,
      'xml:lang': lang,
      ...(direction ? { 'its:dir': direction as 'ltr' | 'rtl' } : {}),
    };
  }
  if (direction) throw new Error('Text direction requires a language tag.');
  return literal(text, qualifier.trim() ? parseIri(qualifier, prefixes) : XSD + 'string');
}

export class ShapeGraph {
  readonly triples: Triple[];
  private readonly subjects = new Map<string, Triple[]>();
  private readonly terms = new Map<string, Term>();
  /**
   * A staged graph is an intermediate state of one edit, which may briefly hold both a
   * replacement structure and the structure it replaces. Only the final graph is bounded.
   */
  constructor(
    triples: readonly Triple[] = [],
    private readonly staged = false,
  ) {
    if (!staged && triples.length > MAX_TRIPLES)
      throw new Error(
        `The shapes graph exceeds ${MAX_TRIPLES} triples. Choose a smaller shapes graph.`,
      );
    const unique = new Map<string, Triple>();
    for (const t of triples) {
      if (!isResource(t[0]) || t[1].type !== 'uri')
        throw new Error('The graph has a triple this editor cannot safely write.');
      unique.set(tripleId(t), t);
    }
    this.triples = [...unique.values()];
    if (!staged && new TextEncoder().encode(JSON.stringify(this.triples)).length > MAX_BYTES)
      throw new Error('The shapes graph exceeds the 2 MiB editor limit.');
    for (const t of this.triples) {
      const key = id(t[0]);
      let statements = this.subjects.get(key);
      if (!statements) {
        statements = [];
        this.subjects.set(key, statements);
      }
      statements.push(t);
      this.terms.set(key, t[0]);
    }
  }
  term(key: string) {
    return this.terms.get(key);
  }
  statements(subject: Term): Triple[] {
    return this.subjects.get(id(subject)) ?? [];
  }
  values(subject: Term, predicate: string): Term[] {
    return this.statements(subject)
      .filter((t) => t[1].value === predicate)
      .map((t) => t[2]);
  }
  one(subject: Term, predicate: string): Term {
    const vs = this.values(subject, predicate);
    if (vs.length !== 1)
      throw new Error(
        `Expected one ${predicate.slice(Math.max(predicate.lastIndexOf('#'), predicate.lastIndexOf('/')) + 1)} value.`,
      );
    return vs[0];
  }
  list(root: Term): Term[] {
    const out: Term[] = [],
      seen = new Set<string>();
    let cell = root;
    while (!same(cell, nil)) {
      if (!isResource(cell) || seen.has(id(cell)) || seen.size >= MAX_TRIPLES)
        throw new Error('This RDF list is cyclic, malformed or too long. Its RDF is preserved.');
      seen.add(id(cell));
      out.push(this.one(cell, first));
      cell = this.one(cell, rest);
    }
    return out;
  }
  path(
    root: Term,
    seen = new Set<string>(),
    depth = 0,
    budget = { left: MAX_PATH_NODES },
  ): ShapePath {
    // Shared operands are expanded once per reference, so an acyclic graph of shared
    // nodes could otherwise describe an exponential tree.
    if (--budget.left < 0)
      throw new Error(
        `The path expands to more than ${MAX_PATH_NODES} steps. Its RDF is preserved.`,
      );
    if (depth > MAX_DEPTH || seen.has(id(root)))
      throw new Error('The path is cyclic or nested more than 64 levels. Its RDF is preserved.');
    if (root.type === 'uri') return { kind: 'predicate', iri: root.value };
    if (root.type !== 'bnode')
      throw new Error('A path must be an IRI or a blank node. Its RDF is preserved.');
    const next = new Set(seen).add(id(root));
    const operators = PATH_OPS.filter(
      (p) => p !== 'predicate' && p !== 'sequence' && this.values(root, SH + p).length,
    );
    const seq = this.values(root, first).length > 0;
    if (operators.length + Number(seq) !== 1)
      throw new Error('The path has missing or competing operators. Its RDF is preserved.');
    const kind: PathKind = seq ? 'sequence' : operators[0];
    if (!seq && this.statements(root).length !== 1)
      throw new Error(
        'This path has additional statements. Its complete RDF is preserved until explicit replacement.',
      );
    const members =
      kind === 'sequence'
        ? this.list(root)
        : kind === 'alternativePath'
          ? this.list(this.one(root, SH + kind))
          : [this.one(root, SH + kind)];
    if ((kind === 'sequence' || kind === 'alternativePath') && members.length < 2)
      throw new Error('Sequence and alternative paths need at least two members.');
    return { kind, children: members.map((m) => this.path(m, next, depth + 1, budget)) };
  }
  /** Generated labels cannot alias an existing blank node, including one only used as an object. */
  fresh(): Term {
    let n = 1;
    const used = new Set(
      this.triples.flatMap((t) =>
        t
          .flatMap(resources)
          .filter((v) => v.type === 'bnode')
          .map((v) => v.value),
      ),
    );
    while (used.has(`visual_${n}`)) n++;
    return { type: 'bnode', value: `visual_${n}` };
  }
  add(...triples: Triple[]): ShapeGraph {
    return new ShapeGraph([...this.triples, ...triples], this.staged);
  }
  replace(subject: Term, predicate: string, values: Term[]): ShapeGraph {
    return new ShapeGraph(
      [
        ...this.triples.filter((t) => !(same(t[0], subject) && t[1].value === predicate)),
        ...values.map((v) => [subject, iri(predicate), v] as Triple),
      ],
      this.staged,
    );
  }
  replaceValue(
    subject: Term,
    predicate: string,
    before: Term | null,
    after: Term | null,
  ): ShapeGraph {
    const values = this.values(subject, predicate).filter((v) => !before || !same(v, before));
    if (after) values.push(after);
    return this.replace(subject, predicate, values);
  }
  /** Only detached, purely structural blank nodes are collected. Annotated/shared RDF stays. */
  prune(root: Term): ShapeGraph {
    const incoming = new Map<string, number>();
    for (const t of this.triples)
      for (const v of resources(t[2])) incoming.set(id(v), (incoming.get(id(v)) ?? 0) + 1);
    const todo = [root],
      removed = new Set<string>();
    while (todo.length) {
      const node = todo.pop()!,
        key = id(node);
      if (node.type !== 'bnode' || incoming.get(key) || removed.has(key)) continue;
      const own = this.statements(node);
      if (!own.length || own.some((t) => !structural.has(String(t[1].value)))) continue;
      removed.add(key);
      for (const t of own) {
        const child = id(t[2]);
        incoming.set(child, (incoming.get(child) ?? 0) - 1);
        todo.push(t[2]);
      }
    }
    return removed.size
      ? new ShapeGraph(
          this.triples.filter((t) => !removed.has(id(t[0]))),
          this.staged,
        )
      : this;
  }
  withList(items: Term[]): { graph: ShapeGraph; root: Term } {
    let g: ShapeGraph = this,
      root: Term = nil;
    for (const item of items.toReversed()) {
      const cell = g.fresh();
      g = g.add([cell, iri(first), item], [cell, iri(rest), root]);
      root = cell;
    }
    return { graph: g, root };
  }
  withPath(path: ShapePath, depth = 0): { graph: ShapeGraph; root: Term } {
    if (depth > MAX_DEPTH) throw new Error('Paths may be nested at most 64 levels.');
    if (path.kind === 'predicate') return { graph: this, root: iri(parseIri(path.iri ?? '', {})) };
    const children = path.children ?? [];
    const multi = path.kind === 'sequence' || path.kind === 'alternativePath';
    if (multi ? children.length < 2 : children.length !== 1)
      throw new Error(
        multi
          ? 'A sequence or alternative needs at least two members.'
          : 'This path needs exactly one child.',
      );
    let g: ShapeGraph = this;
    const roots: Term[] = [];
    for (const child of children) {
      const a = g.withPath(child, depth + 1);
      g = a.graph;
      roots.push(a.root);
    }
    if (path.kind === 'sequence') return g.withList(roots);
    let value = roots[0];
    if (path.kind === 'alternativePath') {
      const list = g.withList(roots);
      g = list.graph;
      value = list.root;
    }
    const root = g.fresh();
    return { graph: g.add([root, iri(SH + path.kind), value]), root };
  }
  setPath(subject: Term, path: ShapePath): ShapeGraph {
    const old = this.values(subject, SH + 'path');
    const a = new ShapeGraph(this.triples, true).withPath(path);
    let g = a.graph.replace(subject, SH + 'path', [a.root]);
    for (const root of old) g = g.prune(root);
    return new ShapeGraph(g.triples);
  }
  setList(subject: Term, predicate: string, before: Term | null, items: Term[] | null): ShapeGraph {
    const staged = new ShapeGraph(this.triples, true);
    const a = items ? staged.withList(items) : { graph: staged, root: null };
    let g = a.graph.replaceValue(subject, predicate, before, a.root);
    if (before) g = g.prune(before);
    return new ShapeGraph(g.triples);
  }
  property(subject: Term): boolean {
    return (
      this.values(subject, SH + 'path').length > 0 ||
      this.values(subject, RDF + 'type').some((t) => same(t, iri(SH + 'PropertyShape')))
    );
  }
  shapes(): Term[] {
    const found = new Map<string, Term>();
    const keep = (t: Term) => {
      if (isResource(t)) found.set(id(t), t);
    };
    for (const [s, p, o] of this.triples) {
      const name = p.type === 'uri' && p.value.startsWith(SH) ? p.value.slice(SH.length) : '';
      if (
        (p.value === RDF + 'type' &&
          (same(o, iri(SH + 'NodeShape')) || same(o, iri(SH + 'PropertyShape')))) ||
        name === 'path' ||
        name.startsWith('target') ||
        shapeLinks.has(name) ||
        (FIELD_BY_NAME.has(name) && FIELD_BY_NAME.get(name)!.group === 'Constraints')
      )
        keep(s);
      if (shapeLinks.has(name)) keep(o);
      if (logicalLinks.has(name)) {
        keep(s);
        try {
          this.list(o).forEach(keep);
        } catch {
          /* malformed lists remain visible */
        }
      }
    }
    return [...found.values()].sort((a, b) => id(a).localeCompare(id(b)));
  }
  links(subject: Term): { term: Term; property: string }[] {
    return this.statements(subject).flatMap(([, p, o]) => {
      const name = p.type === 'uri' ? p.value.slice(SH.length) : '';
      if (p.type !== 'uri' || !p.value.startsWith(SH)) return [];
      if (shapeLinks.has(name) && isResource(o)) return [{ term: o, property: name }];
      if (logicalLinks.has(name)) {
        try {
          return this.list(o)
            .filter(isResource)
            .map((term) => ({ term, property: name }));
        } catch {
          return [];
        }
      }
      return [];
    });
  }
  create(
    kind: 'node' | 'property',
    term: Term = this.fresh(),
    parent?: Term,
  ): { graph: ShapeGraph; term: Term } {
    if (!isResource(term) || this.triples.some((t) => t.some((v) => same(v, term))))
      throw new Error('That shape identity is already used in this graph.');
    let graph = this.add([
      term,
      iri(RDF + 'type'),
      iri(SH + (kind === 'node' ? 'NodeShape' : 'PropertyShape')),
    ]);
    if (parent) graph = graph.add([parent, iri(SH + 'property'), term]);
    return { graph, term };
  }
  /** Remove only the definition, refusing external references; detached structures are safe-collected. */
  remove(subject: Term): ShapeGraph {
    if (
      this.triples.some(
        (t) => resources(t[2]).some((v) => same(v, subject)) && !same(t[0], subject),
      )
    )
      throw new Error(
        'Other statements reference this shape. Remove its links in their forms before deleting the definition.',
      );
    const own = this.statements(subject);
    let g = new ShapeGraph(this.triples.filter((t) => !same(t[0], subject)));
    for (const [, p, o] of own)
      if (
        p.value === SH + 'path' ||
        listKind(FIELD_BY_NAME.get(String(p.value).slice(SH.length))?.kind ?? 'term')
      )
        g = g.prune(o);
    return g;
  }
  turtle(): string {
    // patchTerm escapes RDF 1.2 terms without dropping language direction; blanks use Turtle labels here.
    const term = (t: Term): string =>
      t.type === 'bnode'
        ? `_:${t.value}`
        : t.type === 'triple'
          ? `<<( ${term(t.value.subject)} ${term(t.value.predicate)} ${term(t.value.object)} )>>`
          : patchTerm(t);
    return this.triples.map((t) => t.map(term).join(' ') + ' .').join('\n') + '\n';
  }
}

/** Local shape-syntax diagnostics. Data-dependent semantics stay with the validator. */
export function valueProblem(graph: ShapeGraph, field: Field, value: Term): string | null {
  let values: Term[];
  try {
    values = listKind(field.kind) ? graph.list(value) : [value];
  } catch (e) {
    return (e as Error).message;
  }
  const kind =
    field.kind === 'iris'
      ? 'iri'
      : field.kind === 'shapes'
        ? 'shape'
        : field.kind === 'languages'
          ? 'text'
          : field.kind === 'terms'
            ? 'term'
            : field.kind;
  for (const v of values) {
    if (kind === 'iri' && v.type !== 'uri')
      return 'This value must be an IRI. The stored RDF is preserved.';
    if (kind === 'shape' && !isResource(v))
      return 'A shape reference must be an IRI or blank node. The stored RDF is preserved.';
    if (field.name === 'node' && graph.property(v)) return 'sh:node must refer to a node shape.';
    if (['text', 'integer', 'decimal', 'boolean'].includes(kind) && v.type !== 'literal')
      return 'This value must be a literal. The stored RDF is preserved.';
    if (v.type === 'literal') {
      if (
        kind === 'integer' &&
        (!/^(?:\+?\d+|-0+)$/.test(v.value) || v.datatype !== XSD + 'integer')
      )
        return 'This parameter needs a nonnegative xsd:integer.';
      if (
        kind === 'boolean' &&
        (v.datatype !== XSD + 'boolean' || !/^(true|false|0|1)$/.test(v.value))
      )
        return 'This parameter needs an xsd:boolean.';
      if (kind === 'decimal' && !/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(v.value))
        return 'This parameter needs a decimal number.';
      if (
        ['languageIn', 'pattern', 'flags'].includes(field.name) &&
        (v['xml:lang'] || (v.datatype && v.datatype !== XSD + 'string'))
      )
        return 'This parameter needs plain xsd:string literals.';
      if (
        field.name === 'languageIn' &&
        !/^(?:\*|[A-Za-z]{1,8}(?:-[A-Za-z0-9]{1,8})*)$/.test(v.value)
      )
        return 'Use basic language ranges such as en, fr-CA or *.';
    }
    if (
      ['minExclusive', 'minInclusive', 'maxExclusive', 'maxInclusive'].includes(field.name) &&
      v.type !== 'literal'
    )
      return 'A range bound must be a literal.';
    if (
      field.name === 'nodeKind' &&
      (v.type !== 'uri' ||
        ![
          'IRI',
          'BlankNode',
          'Literal',
          'BlankNodeOrIRI',
          'BlankNodeOrLiteral',
          'IRIOrLiteral',
        ].includes(v.value.slice(SH.length)) ||
        !v.value.startsWith(SH))
    )
      return 'Choose one of the six SHACL node kinds.';
  }
  return null;
}

/** Existing opaque or malformed fields are preserved; only changed invalid fields block. */
export function definitionProblems(graph: ShapeGraph, basis?: ShapeGraph): string[] {
  const problems: string[] = [];
  const originalShapes = new Set(basis?.shapes().map(id));
  const footprint = (g: ShapeGraph, subject: Term, predicate: string) => {
    const rows = g.statements(subject).filter((t) => t[1].value === predicate);
    const todo = rows.flatMap((t) => resources(t[2])).filter((t) => t.type === 'bnode');
    const seen = new Set<string>();
    while (todo.length) {
      const term = todo.pop()!;
      if (seen.has(id(term))) continue;
      seen.add(id(term));
      for (const row of g.statements(term)) {
        rows.push(row);
        todo.push(...resources(row[2]).filter((t) => t.type === 'bnode'));
      }
    }
    return rows.map(tripleId).sort().join('\n');
  };
  const changed = (shape: Term, ...predicates: string[]) =>
    !basis ||
    !originalShapes.has(id(shape)) ||
    predicates.some((p) => footprint(graph, shape, p) !== footprint(basis, shape, p));
  for (const shape of graph.shapes()) {
    const label = id(shape),
      property = graph.property(shape);
    if (property) {
      if (changed(shape, SH + 'path', RDF + 'type')) {
        try {
          graph.path(graph.one(shape, SH + 'path'));
        } catch (e) {
          problems.push(`${label}: ${(e as Error).message}`);
        }
        if (graph.values(shape, RDF + 'type').some((t) => same(t, iri(SH + 'NodeShape'))))
          problems.push(`${label} is declared as both a node and a property shape.`);
      }
    }
    for (const field of FIELDS) {
      if (
        !changed(shape, SH + field.name, ...(field.propertyOnly ? [SH + 'path', RDF + 'type'] : []))
      )
        continue;
      const values = graph.values(shape, SH + field.name);
      if (values.length && field.propertyOnly && !property)
        problems.push(`${label}: sh:${field.name} applies to property shapes.`);
      if (!field.multiple && values.length > 1)
        problems.push(`${label}: sh:${field.name} has more than one value.`);
      for (const v of values) {
        const problem = valueProblem(graph, field, v);
        if (problem) problems.push(`${label}, sh:${field.name}: ${problem}`);
      }
    }
    if (
      changed(shape, SH + 'property') &&
      graph.values(shape, SH + 'property').some((t) => !isResource(t) || !graph.property(t))
    )
      problems.push(`${label}: sh:property must refer to a property shape with a path.`);
  }
  return problems;
}

/** Virtual query selectors cannot name a literal RDF Patch destination. */
function authoringGraph(graph: string | null) {
  if (graph === 'urn:x-arq:DefaultGraph' || graph === 'urn:x-arq:UnionGraph')
    throw new Error(
      'Leave the shapes graph empty for the stored default graph. Virtual default/union selectors are not named authoring graphs.',
    );
}

/** Read the stored default graph, not the possibly configured union default. */
export function graphQuery(graph: string | null): string {
  authoringGraph(graph);
  return `SELECT ?s ?p ?o WHERE { GRAPH ${patchTerm(iri(graph ?? 'urn:x-arq:DefaultGraph'))} { ?s ?p ?o } } LIMIT ${MAX_TRIPLES + 1}`;
}

export function graphResult(result: SparklesResult): ShapeGraph {
  if (
    result.queryType !== 'SELECT' ||
    result.vars?.join(',') !== 's,p,o' ||
    !result.rows ||
    !Number.isSafeInteger(result.meta.totalRows) ||
    result.meta.sentRows !== result.rows.length ||
    result.meta.totalRows !== result.rows.length ||
    result.rows.length > MAX_TRIPLES ||
    result.meta.plan?.complete === false
  )
    throw new Error(
      'The shapes graph read was incomplete or exceeded 5,000 triples. Choose a smaller shapes graph.',
    );
  return new ShapeGraph(
    result.rows.map((row) => {
      if (row.length !== 3 || row.some((t) => t == null))
        throw new Error('The shapes graph read returned an incomplete triple.');
      return row as Triple;
    }),
  );
}

export function delta(
  before: ShapeGraph,
  after: ShapeGraph,
): { added: Triple[]; removed: Triple[] } {
  const a = new Set(before.triples.map(tripleId)),
    b = new Set(after.triples.map(tripleId));
  return {
    added: after.triples.filter((t) => !a.has(tripleId(t))),
    removed: before.triples.filter((t) => !b.has(tripleId(t))),
  };
}

/** The exact branch UUID + head are mandatory: patch ignores If-Match. */
export function shapePatch(
  before: ShapeGraph,
  after: ShapeGraph,
  graph: string | null,
  branchId: string,
  head: number,
): string {
  authoringGraph(graph);
  if (
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(branchId) ||
    !Number.isSafeInteger(head) ||
    head < 0
  )
    throw new Error('The branch has no valid commit identity. Reload before editing.');
  const d = delta(before, after);
  const oldBlanks = new Set(
    before.triples.flatMap((t) =>
      t
        .flatMap(resources)
        .filter((v) => v.type === 'bnode')
        .map((v) => v.value),
    ),
  );
  const term = (t: Term): string =>
    t.type === 'bnode' && !oldBlanks.has(t.value) ? `_:${t.value}` : patchTerm(t);
  const g = graph ? ' ' + patchTerm(iri(graph)) : '';
  const rows = (op: string, ts: Triple[]) => ts.map((t) => `${op} ${t.map(term).join(' ')}${g} .`);
  return [
    `H prev <urn:uuid:${branchId}#commit:${head}> .`,
    'TX .',
    ...rows('D', d.removed),
    ...rows('A', d.added),
    'TC .',
    '',
  ].join('\n');
}

export function sampleNodes(text: string, prefixes: PrefixMap): Term[] {
  const lines = text
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean);
  if (!lines.length)
    throw new Error('Enter at least one sample node, one IRI or blank-node label per line.');
  if (lines.length > MAX_SAMPLES)
    throw new Error(`Validate at most ${MAX_SAMPLES} sample nodes at a time.`);
  return lines.map((s) =>
    s.startsWith('_:') ? inputTerm('term', s, prefixes, 'bnode') : iri(parseIri(s, prefixes)),
  );
}

/** A bounded spanning forest. Repeated references are leaves rather than exponentially
 * many paths through a shared shape graph. The ordinary tree still virtualizes rows. */
export function shapeTree(graph: ShapeGraph): {
  roots: string[];
  nodes: Map<string, { term: Term; reference: boolean; via?: string }>;
  children: Map<string, string[]>;
  parent: Map<string, string>;
  omitted: number;
} {
  const shapes = graph
    .shapes()
    .sort(
      (a, b) => Number(graph.property(a)) - Number(graph.property(b)) || id(a).localeCompare(id(b)),
    );
  const incoming = new Set<string>();
  for (const shape of shapes) for (const link of graph.links(shape)) incoming.add(id(link.term));
  const roots: string[] = [],
    nodes = new Map<string, { term: Term; reference: boolean; via?: string }>(),
    children = new Map<string, string[]>(),
    parent = new Map<string, string>();
  const seen = new Set<string>();
  let references = 0,
    omitted = 0;
  const visit = (root: Term) => {
    const key = id(root);
    if (seen.has(key)) return;
    roots.push(key);
    seen.add(key);
    nodes.set(key, { term: root, reference: false });
    const queue = [root];
    for (let at = 0; at < queue.length; at++) {
      const term = queue[at],
        kids: string[] = [];
      for (const link of graph.links(term)) {
        const actual = id(link.term),
          reference = seen.has(actual);
        if (reference && references >= MAX_TREE_REFERENCES) {
          omitted++;
          continue;
        }
        const row = reference ? `ref:${++references}:${actual}` : actual;
        nodes.set(row, { term: link.term, reference, via: link.property });
        parent.set(row, id(term));
        kids.push(row);
        if (!reference) {
          seen.add(actual);
          queue.push(link.term);
        }
      }
      children.set(id(term), kids);
    }
  };
  shapes.filter((s) => !incoming.has(id(s))).forEach(visit);
  shapes.forEach(visit);
  return { roots, nodes, children, parent, omitted };
}

/** Session-only draft history. Each state retains the complete RDF graph. */
export class ShapeDraft {
  readonly states: ShapeGraph[];
  readonly cursor: number;
  constructor(
    readonly basis: ShapeGraph,
    states = [basis],
    cursor = 0,
  ) {
    this.states = states;
    this.cursor = cursor;
  }
  get graph() {
    return this.states[this.cursor];
  }
  get dirty() {
    const d = delta(this.basis, this.graph);
    return !!(d.added.length || d.removed.length);
  }
  change(graph: ShapeGraph): ShapeDraft {
    const d = delta(this.graph, graph);
    if (!d.added.length && !d.removed.length) return this;
    // Bound retained draft history to 30 graph snapshots; the original basis stays pinned.
    const states = [...this.states.slice(0, this.cursor + 1), graph].slice(-30);
    return new ShapeDraft(this.basis, states, states.length - 1);
  }
  undo() {
    return new ShapeDraft(this.basis, this.states, Math.max(0, this.cursor - 1));
  }
  redo() {
    return new ShapeDraft(
      this.basis,
      this.states,
      Math.min(this.states.length - 1, this.cursor + 1),
    );
  }
}
